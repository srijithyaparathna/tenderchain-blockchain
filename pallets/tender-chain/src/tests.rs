//! Tests for TenderChain.
//!
//! Coverage targets spec §9: state-machine gate transitions, commit/reveal
//! validation including mismatch voiding, bond lifecycle, score-variance
//! flagging, and challenge suspension — plus the three end-to-end flows in §6.

use crate::{mock::*, types::*, Error, Event};
use frame_support::{assert_noop, assert_ok, traits::Get, BoundedVec};

const ENTITY: AccountId = 1;
const OFFICER: AccountId = 2;
const BIDDER_A: AccountId = 10;
const BIDDER_B: AccountId = 11;
const BIDDER_C: AccountId = 12;
const EVAL_1: AccountId = 20;
const EVAL_2: AccountId = 21;

const NOTICE: Hash256 = [1u8; 32];
const CRITERIA: Hash256 = [2u8; 32];
const TITLE: &[u8] = b"Arterial road resurfacing";
const SUMMARY: &[u8] = b"Resurfacing of 4km of arterial road, including drainage.";
const SALT_A: Hash256 = [0xAAu8; 32];
const SALT_B: Hash256 = [0xBBu8; 32];
const DOCS_A: Hash256 = [0xA1u8; 32];
const DOCS_B: Hash256 = [0xB1u8; 32];
const RATIONALE: Hash256 = [9u8; 32];
const CONTRACT: Hash256 = [8u8; 32];
const SALT_C: Hash256 = [0xCCu8; 32];
const DOCS_C: Hash256 = [0xC3u8; 32];
/// Criterion ids are hashes of each criterion's definition; any distinct
/// 32-byte values stand in for them here.
const C1: CriterionId = [0xC1u8; 32];
const C2: CriterionId = [0xC2u8; 32];

/// The id minted by the most recent `create_tender`. Ids are hashes, so tests
/// read them back from the event instead of assuming a counter value.
fn last_tender_id() -> TenderId {
	System::events()
		.iter()
		.rev()
		.find_map(|r| match &r.event {
			RuntimeEvent::TenderChain(Event::TenderCreated { tender_id, .. }) => Some(*tender_id),
			_ => None,
		})
		.expect("no TenderCreated event")
}

/// Question ids minted on `tender`, in the order they were asked.
fn question_ids(tender: TenderId) -> Vec<QuestionId> {
	System::events()
		.iter()
		.filter_map(|r| match &r.event {
			RuntimeEvent::TenderChain(Event::QuestionAsked { tender_id, question_id })
				if *tender_id == tender =>
				Some(*question_id),
			_ => None,
		})
		.collect()
}

/// Challenge ids minted on `tender`, in the order they were lodged.
fn challenge_ids(tender: TenderId) -> Vec<ChallengeId> {
	System::events()
		.iter()
		.filter_map(|r| match &r.event {
			RuntimeEvent::TenderChain(Event::ChallengeLodged { tender_id, challenge_id, .. })
				if *tender_id == tender =>
				Some(*challenge_id),
			_ => None,
		})
		.collect()
}

/// Grounds and resolutions are readable text on chain, not hashes, so the tests
/// pass the words a challenger and a resolver would actually write.
fn grounds() -> Vec<u8> {
	b"The winning bid was scored against criteria not published in the notice.".to_vec()
}

fn resolution() -> Vec<u8> {
	b"Dismissed: the criteria in question were published in addendum 2.".to_vec()
}

fn weights() -> Vec<CriterionWeight> {
	vec![
		CriterionWeight { criterion_id: C1, weight_percent: 60 },
		CriterionWeight { criterion_id: C2, weight_percent: 40 },
	]
}

/// publish=2, questions close=5, submissions close=10, opening 10..15
fn gates() -> TenderGates<u64> {
	TenderGates {
		publish_at: 1,
		questions_close_at: 5,
		submission_close_at: 10,
		opening_at: 10,
		opening_end_at: 15,
	}
}

fn bond(amount: Balance) -> BondTerms<Balance> {
	BondTerms { amount, forfeit_on_non_reveal: true, forfeit_on_withdrawal: false }
}

/// `(n, amount)` becomes a line whose item id is `[n; 32]`.
fn prices(v: Vec<(u8, Balance)>) -> Vec<PriceLine<Balance>> {
	v.into_iter().map(|(n, amount)| PriceLine { item_id: [n; 32], amount }).collect()
}

fn bounded_prices(v: Vec<PriceLine<Balance>>) -> BoundedVec<PriceLine<Balance>, MaxPriceLines> {
	v.try_into().unwrap()
}

fn create(bond_amount: Balance) -> TenderId {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Rft,
		BidMode::Sealed,
		TITLE.to_vec(),
		SUMMARY.to_vec(),
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
	last_tender_id()
}

fn create_and_publish(bond_amount: Balance) -> TenderId {
	let id = create(bond_amount);
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
	id
}

fn commitment_for(bidder: AccountId, docs: Hash256, p: Vec<PriceLine<Balance>>, salt: Hash256) -> Hash256 {
	TenderChain::compute_commitment(&bidder, &docs, &bounded_prices(p), &salt)
}

/// Take the tender through to `Evaluation` with two valid sealed bids revealed.
fn to_evaluation_with_two_bids(bond_amount: Balance) -> TenderId {
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

/// As `to_evaluation_with_two_bids`, with BIDDER_C as a third valid bidder.
fn to_evaluation_with_three_bids(bond_amount: Balance) -> TenderId {
	let id = create_and_publish(bond_amount);
	let bids = [
		(BIDDER_A, DOCS_A, SALT_A, prices(vec![(1, 500)])),
		(BIDDER_B, DOCS_B, SALT_B, prices(vec![(1, 600)])),
		(BIDDER_C, DOCS_C, SALT_C, prices(vec![(1, 700)])),
	];
	for (who, docs, salt, p) in bids.iter() {
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(*who),
			id,
			commitment_for(*who, *docs, p.clone(), *salt)
		));
	}
	run_to_block(11);
	assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
	for (who, docs, salt, p) in bids.into_iter() {
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(who), id, docs, p, salt));
	}
	run_to_block(16);
	id
}

fn appoint_and_activate(id: TenderId, who: AccountId) {
	assert_ok!(TenderChain::appoint_evaluator(RuntimeOrigin::signed(OFFICER), id, who, [7u8; 32]));
	assert_ok!(TenderChain::declare_conflict(RuntimeOrigin::signed(who), id, [3u8; 32]));
	assert_ok!(TenderChain::activate_evaluator(RuntimeOrigin::signed(OFFICER), id, who));
}

fn score(id: TenderId, evaluator: AccountId, bidder: AccountId, s1: u8, s2: u8) {
	assert_ok!(TenderChain::submit_scores(
		RuntimeOrigin::signed(evaluator),
		id,
		bidder,
		vec![
			CriterionScore { criterion_id: C1, score: s1 },
			CriterionScore { criterion_id: C2, score: s2 },
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
		System::assert_has_event(Event::TenderCreated { tender_id: id, officer: OFFICER, title: TITLE.to_vec() }.into());
	});
}

#[test]
fn create_tender_stores_readable_title_and_summary() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		let t = TenderChain::tenders(id).unwrap();
		// The point of the change: chain state alone is readable, with no
		// off-chain store to dereference.
		assert_eq!(t.title.into_inner(), TITLE.to_vec());
		assert_eq!(t.summary.into_inner(), SUMMARY.to_vec());
	});
}

#[test]
fn create_tender_rejects_an_empty_title() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				vec![],
				SUMMARY.to_vec(),
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
			Error::<Test>::TitleEmpty
		);
	});
}

#[test]
fn create_tender_rejects_an_oversized_title_rather_than_truncating() {
	new_test_ext().execute_with(|| {
		let too_long = vec![b'x'; <Test as crate::Config>::MaxTitleLen::get() as usize + 1];
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				too_long,
				SUMMARY.to_vec(),
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
			Error::<Test>::TitleTooLong
		);
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
				TITLE.to_vec(),
				SUMMARY.to_vec(),
				NOTICE,
				CRITERIA,
				vec![CriterionWeight { criterion_id: C1, weight_percent: 90 }],
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
				TITLE.to_vec(),
				SUMMARY.to_vec(),
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
				TITLE.to_vec(),
				SUMMARY.to_vec(),
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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

		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [6u8; 32]));

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
					CriterionScore { criterion_id: C1, score: 50 },
					CriterionScore { criterion_id: C2, score: 50 }
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
					CriterionScore { criterion_id: C1, score: 99 },
					CriterionScore { criterion_id: C2, score: 99 }
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
					CriterionScore { criterion_id: C1, score: 50 },
					CriterionScore { criterion_id: [0x99u8; 32], score: 50 }
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
				vec![CriterionScore { criterion_id: C1, score: 50 }],
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
					CriterionScore { criterion_id: C1, score: 200 },
					CriterionScore { criterion_id: C2, score: 50 }
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
				criterion_id: C1,
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
					CriterionScore { criterion_id: C1, score: 50 },
					CriterionScore { criterion_id: C2, score: 50 }
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
			Error::<Test>::AwardAuthorityRequired
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
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));
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
			challenge_ids(id)[0],
			false,
			resolution()
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
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));

		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			challenge_ids(id)[0],
			true,
			resolution()
		));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));

		let c = TenderChain::challenges(id, challenge_ids(id)[0]).unwrap();
		assert!(matches!(c.state, ChallengeState::Upheld));
		assert_eq!(c.resolution.unwrap().into_inner(), resolution());
	});
}

#[test]
fn challenge_grounds_and_resolution_are_readable_on_chain() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		assert_ok!(TenderChain::lodge_challenge(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			grounds(),
			Some([0xE0u8; 32])
		));

		// The point of the change: raw chain state carries the words, so an
		// auditor with the node alone can read what was alleged.
		let c = TenderChain::challenges(id, challenge_ids(id)[0]).unwrap();
		assert_eq!(c.grounds.clone().into_inner(), grounds());
		assert_eq!(c.evidence_hash, Some([0xE0u8; 32]));
		assert!(c.resolution.is_none());

		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			challenge_ids(id)[0],
			false,
			resolution()
		));
		let c = TenderChain::challenges(id, challenge_ids(id)[0]).unwrap();
		assert_eq!(c.resolution.unwrap().into_inner(), resolution());
	});
}

#[test]
fn empty_or_oversized_grounds_are_rejected() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, vec![], None),
			Error::<Test>::GroundsEmpty
		);
		// Rejected, not truncated: the challenger must see that it did not fit.
		let too_long = vec![b'x'; MaxGroundsLen::get() as usize + 1];
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, too_long, None),
			Error::<Test>::GroundsTooLong
		);
	});
}

#[test]
fn empty_or_oversized_resolution_is_rejected() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::lodge_challenge(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			grounds(),
			None
		));

		assert_noop!(
			TenderChain::resolve_challenge(RuntimeOrigin::root(), id, challenge_ids(id)[0], false, vec![]),
			Error::<Test>::ResolutionEmpty
		);
		let too_long = vec![b'x'; MaxResolutionLen::get() as usize + 1];
		assert_noop!(
			TenderChain::resolve_challenge(RuntimeOrigin::root(), id, challenge_ids(id)[0], false, too_long),
			Error::<Test>::ResolutionTooLong
		);
		// A rejected ruling leaves the challenge open and execution suspended.
		let c = TenderChain::challenges(id, challenge_ids(id)[0]).unwrap();
		assert!(matches!(c.state, ChallengeState::Open));
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
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None),
			Error::<Test>::ChallengeWindowClosed
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
			question_ids(id)[0],
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
				criterion_id: C1,
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
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_C), id, grounds(), None),
			Error::<Test>::NotAParticipant
		);
		// The losing bidder has standing.
		assert_ok!(TenderChain::lodge_challenge(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			grounds(),
			None
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
				format!("Ground {i}").into_bytes(),
				None
			));
		}
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None),
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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
		let id = last_tender_id();
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
		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
		let panel = TenderChain::panel_id_of(&id);

		// A stranger cannot order against the agency's standing offer.
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(BIDDER_C), panel, BIDDER_A, [0x77u8; 32]),
			Error::<Test>::NotOfficer
		);
		// The officer can.
		assert_ok!(TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_A, [0x77u8; 32]));
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
				criterion_id: C1,
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
		let id = to_evaluation_with_three_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		// Both unsuccessful bidders challenge.
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_C), id, grounds(), None));

		// First upheld -> remitted for re-evaluation.
		assert_ok!(TenderChain::resolve_challenge(RuntimeOrigin::root(), id, challenge_ids(id)[0], true, resolution()));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));

		// Second dismissed -> must stay remitted, not snap back to Awarded.
		assert_ok!(TenderChain::resolve_challenge(RuntimeOrigin::root(), id, challenge_ids(id)[1], false, resolution()));
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
				TITLE.to_vec(),
				SUMMARY.to_vec(),
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

fn create_open_rfq() -> TenderId {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Rfq,
		BidMode::Open,
		TITLE.to_vec(),
		SUMMARY.to_vec(),
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
	let id = last_tender_id();
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
	id
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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
		let id = last_tender_id();
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::ask_question(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			[0x11u8; 32],
			SALT_A
		));

		let q = TenderChain::questions(id, question_ids(id)[0]).unwrap();
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
		let q = TenderChain::questions(id, question_ids(id)[0]).unwrap();
		assert_eq!(q.author, QuestionAuthor::Open(BIDDER_A));
	});
}

// ---------------------------------------------------------------
// Multi-stage: EOI -> shortlist -> RFT (spec §2.2)
// ---------------------------------------------------------------

/// Take an EOI to Evaluation with two responders, then shortlist one.
fn eoi_to_shortlist() -> TenderId {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Eoi,
		BidMode::Sealed,
		TITLE.to_vec(),
		SUMMARY.to_vec(),
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
	let eoi = last_tender_id();
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
			NOTICE,
			CRITERIA,
			weights(),
			TenderGates {
				publish_at: 16,
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
		let rft = last_tender_id();
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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
		let id = last_tender_id();
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

		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [0x5Eu8; 32]));
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
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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
		let id = last_tender_id();
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

		// Multi-award establishes the pool — but only once it survives standstill.
		assert_ok!(TenderChain::award(
			RuntimeOrigin::root(),
			id,
			vec![BIDDER_A, BIDDER_B],
			RATIONALE
		));
		let panel = TenderChain::panel_id_of(&id);
		assert!(TenderChain::panel_pool(panel, BIDDER_A).is_none());

		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
		assert!(TenderChain::panel_pool(panel, BIDDER_A).is_some());
		assert!(TenderChain::panel_pool(panel, BIDDER_B).is_some());
		assert_eq!(TenderChain::panel_tender(panel), Some(id));

		assert_ok!(TenderChain::call_off(
			RuntimeOrigin::signed(OFFICER),
			panel,
			BIDDER_A,
			[0xEEu8; 32]
		));
		// A non-member cannot receive a call-off.
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_C, [0xEEu8; 32]),
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

// ---------------------------------------------------------------
// Hashed identifiers
// ---------------------------------------------------------------

/// Two tenders from the same officer in the same block still get distinct
/// ids, and neither id is a disguised counter.
#[test]
fn tender_ids_are_unique_hashes_not_counters() {
	new_test_ext().execute_with(|| {
		let first = create(0);
		let second = create(0);
		assert_ne!(first, second);
		for id in [first, second] {
			assert_ne!(id, [0u8; 32]);
			// Not a little-endian counter padded out to 32 bytes.
			assert!(id[4..].iter().any(|b| *b != 0));
		}
		assert_eq!(TenderChain::tenders(first).unwrap().created_at, 1);
	});
}

#[test]
fn question_and_challenge_ids_are_distinct_hashes() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::ask_question(RuntimeOrigin::signed(BIDDER_A), id, [0x11u8; 32], SALT_A));
		assert_ok!(TenderChain::ask_question(RuntimeOrigin::signed(BIDDER_B), id, [0x12u8; 32], SALT_B));
		let qs = question_ids(id);
		assert_eq!(qs.len(), 2);
		assert_ne!(qs[0], qs[1]);
		// Domain separation: a question id never equals its tender's id.
		assert!(!qs.contains(&id));
		assert!(TenderChain::questions(id, qs[1]).is_some());
	});
}

#[test]
fn panel_id_is_derived_from_its_tender() {
	new_test_ext().execute_with(|| {
		let a = create(0);
		let b = create(0);
		assert_eq!(TenderChain::panel_id_of(&a), TenderChain::panel_id_of(&a));
		assert_ne!(TenderChain::panel_id_of(&a), TenderChain::panel_id_of(&b));
		assert_ne!(TenderChain::panel_id_of(&a), a);
	});
}

#[test]
fn duplicate_criterion_ids_are_rejected_at_creation() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				TITLE.to_vec(),
				SUMMARY.to_vec(),
				NOTICE,
				CRITERIA,
				vec![
					CriterionWeight { criterion_id: C1, weight_percent: 50 },
					CriterionWeight { criterion_id: C1, weight_percent: 50 },
				],
				gates(),
				20,
				vec![],
				0,
				bond(0),
				false,
				None,
			),
			Error::<Test>::DuplicateCriterion
		);
	});
}

/// Same length as the locked criteria and every entry a known criterion, but
/// C1 scored twice and C2 skipped.
#[test]
fn a_scoresheet_cannot_score_one_criterion_twice() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: C1, score: 90 },
					CriterionScore { criterion_id: C1, score: 90 },
				],
				[0u8; 32]
			),
			Error::<Test>::DuplicateCriterion
		);
	});
}

// ---------------------------------------------------------------
// Spec conformance fixes
// ---------------------------------------------------------------

/// Spec §1.2: publication is a block gate, not an officer's choice of moment.
#[test]
fn a_tender_cannot_publish_before_its_publish_gate() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Rft,
			BidMode::Sealed,
			TITLE.to_vec(),
			SUMMARY.to_vec(),
			NOTICE,
			CRITERIA,
			weights(),
			TenderGates {
				publish_at: 3,
				questions_close_at: 5,
				submission_close_at: 10,
				opening_at: 10,
				opening_end_at: 15,
			},
			20,
			vec![],
			0,
			bond(0),
			false,
			None,
		));
		let id = last_tender_id();
		assert_noop!(
			TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id),
			Error::<Test>::PublishTooEarly
		);
		run_to_block(3);
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
	});
}

/// Answers are what every bidder priced against; rewriting one would be a
/// private clarification by another name.
#[test]
fn a_published_answer_cannot_be_rewritten() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::ask_question(RuntimeOrigin::signed(BIDDER_A), id, [0x11u8; 32], SALT_A));
		let q = question_ids(id)[0];
		assert_ok!(TenderChain::answer_question(RuntimeOrigin::signed(OFFICER), id, q, [0x12u8; 32]));
		assert_noop!(
			TenderChain::answer_question(RuntimeOrigin::signed(OFFICER), id, q, [0x13u8; 32]),
			Error::<Test>::AlreadyAnswered
		);
		assert_noop!(
			TenderChain::answer_question(RuntimeOrigin::signed(OFFICER), id, [0xFFu8; 32], [0x13u8; 32]),
			Error::<Test>::QuestionNotFound
		);
	});
}

/// Spec §8: mismatch handling follows the bond terms. A bidder who saw rivals'
/// prices must not escape `forfeit_on_non_reveal` by revealing garbage.
#[test]
fn a_mismatched_reveal_forfeits_like_a_non_reveal() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		let entity_before = Balances::free_balance(ENTITY);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, prices(vec![(1, 500)]), SALT_A)
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			prices(vec![(1, 999)]),
			SALT_A
		));
		run_to_block(16);

		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::free_balance(ENTITY), entity_before + bond_amount);
		System::assert_has_event(
			Event::BondForfeited { tender_id: id, bidder: BIDDER_A, amount: bond_amount }.into(),
		);
	});
}

/// An open bid is recorded as revealed at submission. Withdrawing it must take
/// that record with it, or the withdrawn bidder stays awardable.
#[test]
fn a_withdrawn_open_bid_cannot_be_awarded() {
	new_test_ext().execute_with(|| {
		let id = create_open_rfq();
		assert_ok!(TenderChain::submit_open_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, prices(vec![(1, 500)])));
		assert_ok!(TenderChain::submit_open_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_B, prices(vec![(1, 600)])));
		assert_ok!(TenderChain::withdraw_commitment(RuntimeOrigin::signed(BIDDER_A), id));
		assert!(TenderChain::reveals(id, BIDDER_A).is_none());

		run_to_block(16);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE),
			Error::<Test>::InvalidAwardee
		);
	});
}

#[test]
fn the_same_awardee_cannot_be_named_twice() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A, BIDDER_A], RATIONALE),
			Error::<Test>::DuplicateAwardee
		);
	});
}

#[test]
fn an_eoi_concludes_with_a_shortlist_not_an_award() {
	new_test_ext().execute_with(|| {
		let eoi = eoi_to_shortlist();
		appoint_and_activate(eoi, EVAL_1);
		appoint_and_activate(eoi, EVAL_2);
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), eoi, vec![BIDDER_A], RATIONALE),
			Error::<Test>::EoiUsesShortlist
		);
	});
}

/// Spec §6.3: challenges come from unsuccessful bidders.
#[test]
fn an_awardee_cannot_challenge_their_own_award() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_A), id, grounds(), None),
			Error::<Test>::AwardeeCannotChallenge
		);
	});
}

/// An outcome record outlives cancellation. Challenging it afterwards must not
/// flip the cancelled tender back to `Challenged`.
#[test]
fn a_cancelled_award_cannot_be_challenged() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [0x5Eu8; 32]));
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None),
			Error::<Test>::BadState
		);
	});
}

/// Cancelling while a challenge is open, then ruling on it, must leave the
/// tender cancelled. The ruling is still recorded on the challenge.
#[test]
fn resolving_a_challenge_does_not_revive_a_cancelled_tender() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));
		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [0x5Eu8; 32]));

		let c = challenge_ids(id)[0];
		assert_ok!(TenderChain::resolve_challenge(RuntimeOrigin::root(), id, c, true, resolution()));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Cancelled));
		assert!(matches!(TenderChain::challenges(id, c).unwrap().state, ChallengeState::Upheld));
	});
}

/// Spec §4.1: cancellation is an entity-authority call, not the officer's.
#[test]
fn only_the_entity_may_cancel() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_noop!(
			TenderChain::cancel_tender(RuntimeOrigin::signed(OFFICER), id, [6u8; 32]),
			Error::<Test>::NotEntity
		);
		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [6u8; 32]));
	});
}

#[test]
fn a_conflict_declaration_is_locked_once_scoring_is_active() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		assert_noop!(
			TenderChain::declare_conflict(RuntimeOrigin::signed(EVAL_1), id, [0x44u8; 32]),
			Error::<Test>::EvaluatorAlreadyActive
		);
	});
}

/// Panel members are admitted at execution, so an award overturned during
/// standstill never reaches the pool.
#[test]
fn an_overturned_panel_award_admits_nobody() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Panel,
			BidMode::Sealed,
			TITLE.to_vec(),
			SUMMARY.to_vec(),
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
		let id = last_tender_id();
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		let pa = prices(vec![(1, 500)]);
		let pb = prices(vec![(1, 600)]);
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)));
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_B), id, commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_B, pb, SALT_B));
		run_to_block(16);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);

		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));
		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			challenge_ids(id)[0],
			true,
			resolution()
		));
		assert!(TenderChain::panel_pool(TenderChain::panel_id_of(&id), BIDDER_A).is_none());
	});
}

/// More gates due in a run of blocks than those blocks can hold. The old
/// deferral pushed overflow one block ahead and dropped it if that block was
/// also full, stranding tenders in `QaWindow`. Every tender must still move.
#[test]
fn deadline_overflow_is_carried_forward_not_dropped() {
	new_test_ext().execute_with(|| {
		// Mock: 8 transitions per block, 32 deadlines per block. 20 tenders close
		// questions at block 5 and 30 at block 6, so both blocks overflow.
		let mut ids = Vec::new();
		for i in 0..50u64 {
			let q_close = if i < 20 { 5 } else { 6 };
			assert_ok!(TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				TITLE.to_vec(),
				SUMMARY.to_vec(),
				NOTICE,
				CRITERIA,
				weights(),
				// Later gates spread one per block so only questions-close collides.
				TenderGates {
					publish_at: 1,
					questions_close_at: q_close,
					submission_close_at: 100 + i,
					opening_at: 100 + i,
					opening_end_at: 200 + i,
				},
				20,
				vec![],
				0,
				bond(0),
				false,
				None,
			));
			let id = last_tender_id();
			assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
			ids.push(id);
		}

		run_to_block(20);
		for id in ids {
			assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Submission));
		}
		assert!(!System::events().iter().any(|e| matches!(
			e.event,
			RuntimeEvent::TenderChain(Event::GateDropped { .. })
		)));
	});
}

// ---------------------------------------------------------------
// Integration seams (spec §7)
// ---------------------------------------------------------------

/// Module 13: publication is a public notice; award and execution mail every
/// bidder (spec §4.1 "notify all bidders via system mail").
#[test]
fn lifecycle_notices_reach_every_bidder() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		assert_eq!(NoticeLog::get()[0], (id, TenderNotice::Published, vec![]));

		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));

		let sent: Vec<TenderNotice> = NoticeLog::get().into_iter().map(|(_, n, _)| n).collect();
		assert_eq!(
			sent,
			vec![
				TenderNotice::Published,
				TenderNotice::Awarded,
				TenderNotice::StandstillClosed,
				TenderNotice::ContractExecuted,
			]
		);
		for (_, notice, to) in NoticeLog::get().into_iter().skip(1) {
			assert_eq!(to, vec![BIDDER_A, BIDDER_B], "{notice:?} must reach both bidders");
		}
	});
}

/// After an upheld challenge the award no longer stands, so bidders must not
/// be told its standstill closed.
#[test]
fn no_standstill_notice_after_an_upheld_challenge() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), None));
		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			challenge_ids(id)[0],
			true,
			resolution()
		));
		run_to_block(100);
		assert!(!NoticeLog::get().iter().any(|(_, n, _)| *n == TenderNotice::StandstillClosed));
	});
}

/// Module 2: a document hash that DNC does not hold is rejected wherever the
/// officer or a challenger publishes one.
#[test]
fn unanchored_documents_are_rejected() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				TITLE.to_vec(),
				SUMMARY.to_vec(),
				UNANCHORED,
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
			Error::<Test>::DocumentNotAnchored
		);

		let id = create_and_publish(0);
		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, UNANCHORED, None),
			Error::<Test>::DocumentNotAnchored
		);
		assert_noop!(
			TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, UNANCHORED),
			Error::<Test>::DocumentNotAnchored
		);
	});
}

#[test]
fn unanchored_rationale_contract_and_evidence_are_rejected() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], UNANCHORED),
			Error::<Test>::DocumentNotAnchored
		);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, grounds(), Some(UNANCHORED)),
			Error::<Test>::DocumentNotAnchored
		);
		run_to_block(100);
		assert_noop!(
			TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, UNANCHORED),
			Error::<Test>::DocumentNotAnchored
		);
	});
}

/// Module 10: the winner's contract, the entity's conduct and a forfeited bond
/// are all reported.
#[test]
fn reputation_facts_are_reported() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
		assert_eq!(
			ReputationLog::get(),
			vec![
				(id, BIDDER_A, ReputationFact::ContractWon),
				(id, ENTITY, ReputationFact::ContractAwarded),
			]
		);
	});
}

#[test]
fn a_forfeited_bond_and_a_cancelled_tender_are_reported() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(5_000);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, prices(vec![(1, 500)]), SALT_A)
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		run_to_block(16); // BIDDER_A never reveals
		assert_eq!(ReputationLog::get(), vec![(id, BIDDER_A, ReputationFact::BondForfeited)]);

		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [6u8; 32]));
		assert_eq!(ReputationLog::get()[1], (id, ENTITY, ReputationFact::TenderCancelled));
	});
}

/// Withdrawing a draft that was never published is not entity misconduct.
#[test]
fn cancelling_an_unpublished_draft_is_not_reported() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(ENTITY), id, [6u8; 32]));
		assert!(ReputationLog::get().is_empty());
	});
}

// ---------------------------------------------------------------
// Spec error names, call-off storage, jurisdiction policy (§4.3, §8)
// ---------------------------------------------------------------

/// Spec §4.3 `BondRequired`: a bidder who cannot cover the bond cannot commit.
#[test]
fn a_bidder_who_cannot_cover_the_bond_gets_bond_required() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(5_000_000); // more than any funded account holds
		assert_noop!(
			TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [1u8; 32]),
			Error::<Test>::BondRequired
		);
	});
}

/// Spec §4.3 `CriteriaLocked`: criteria can be amended in draft, never after.
#[test]
fn criteria_can_be_amended_in_draft_and_are_locked_after_publication() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		let amended = vec![
			CriterionWeight { criterion_id: C1, weight_percent: 30 },
			CriterionWeight { criterion_id: C2, weight_percent: 70 },
		];
		assert_ok!(TenderChain::amend_criteria(RuntimeOrigin::signed(OFFICER), id, [0x33u8; 32], amended.clone()));
		let t = TenderChain::tenders(id).unwrap();
		assert_eq!(t.criteria_hash, [0x33u8; 32]);
		assert_eq!(t.weights.into_inner(), amended);

		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_noop!(
			TenderChain::amend_criteria(RuntimeOrigin::signed(OFFICER), id, CRITERIA, weights()),
			Error::<Test>::CriteriaLocked
		);
	});
}

#[test]
fn an_amendment_is_validated_like_creation() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		assert_noop!(
			TenderChain::amend_criteria(
				RuntimeOrigin::signed(OFFICER),
				id,
				CRITERIA,
				vec![CriterionWeight { criterion_id: C1, weight_percent: 90 }]
			),
			Error::<Test>::WeightsInvalid
		);
		assert_noop!(
			TenderChain::amend_criteria(RuntimeOrigin::signed(BIDDER_A), id, CRITERIA, weights()),
			Error::<Test>::NotOfficer
		);
	});
}

/// Panel with BIDDER_A admitted and executed; returns (tender, panel).
fn executed_panel() -> (TenderId, PanelId) {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Panel,
		BidMode::Sealed,
		TITLE.to_vec(),
		SUMMARY.to_vec(),
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
	let id = last_tender_id();
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
	let pa = prices(vec![(1, 500)]);
	assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)));
	run_to_block(11);
	assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
	assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
	run_to_block(16);
	appoint_and_activate(id, EVAL_1);
	appoint_and_activate(id, EVAL_2);
	assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
	run_to_block(100);
	assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
	(id, TenderChain::panel_id_of(&id))
}

/// Call-offs are stored under hashed ids, bounded, and mailed to the supplier.
#[test]
fn call_offs_are_stored_under_hashed_ids() {
	new_test_ext().execute_with(|| {
		let (id, panel) = executed_panel();
		assert_ok!(TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_A, [0x71u8; 32]));
		assert_ok!(TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_A, [0x72u8; 32]));

		let first = TenderChain::call_off_id_for(&panel, 0);
		let second = TenderChain::call_off_id_for(&panel, 1);
		assert_ne!(first, second);
		let rec = TenderChain::call_offs(panel, first).unwrap();
		assert_eq!(rec.supplier, BIDDER_A);
		assert_eq!(rec.order_hash, [0x71u8; 32]);
		assert_eq!(rec.placed_by, OFFICER);
		System::assert_has_event(
			Event::CallOffPlaced {
				panel_id: panel,
				call_off_id: second,
				supplier: BIDDER_A,
				order_hash: [0x72u8; 32],
			}
			.into(),
		);
		assert!(NoticeLog::get().contains(&(id, TenderNotice::CallOffPlaced, vec![BIDDER_A])));

		// MaxCallOffs is 2 in the mock.
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_A, [0x73u8; 32]),
			Error::<Test>::TooManyCallOffs
		);
	});
}

#[test]
fn an_unanchored_call_off_order_is_rejected() {
	new_test_ext().execute_with(|| {
		let (_, panel) = executed_panel();
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(OFFICER), panel, BIDDER_A, UNANCHORED),
			Error::<Test>::DocumentNotAnchored
		);
	});
}

fn policy(
	min_standstill: u64,
	min_submission_period: u64,
	addendum_response_window: u64,
	max_close_extension: Option<u64>,
) -> ProcurementPolicy<u64> {
	ProcurementPolicy { min_standstill, min_submission_period, addendum_response_window, max_close_extension }
}

#[test]
fn only_the_governed_origin_sets_policy() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			TenderChain::set_policy(RuntimeOrigin::signed(OFFICER), policy(30, 0, 0, None)),
			sp_runtime::DispatchError::BadOrigin
		);
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(30, 0, 0, None)));
		assert_eq!(TenderChain::policy(), policy(30, 0, 0, None));
	});
}

/// Spec §8: a mandatory standstill below the jurisdiction's minimum is refused.
#[test]
fn policy_enforces_the_mandatory_standstill() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(30, 0, 0, None)));
		// The test helper uses a 20-block standstill.
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				TITLE.to_vec(),
				SUMMARY.to_vec(),
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
			Error::<Test>::StandstillTooShort
		);
	});
}

/// The minimum submission period is measured from actual publication, so a
/// draft that complied when created cannot be published late to squeeze the
/// market.
#[test]
fn publishing_late_cannot_squeeze_the_submission_period() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(0, 8, 0, None)));
		// publish_at 1, close 10: nine blocks — complies at creation.
		let id = create(0);
		run_to_block(3); // now only seven blocks remain
		assert_noop!(
			TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id),
			Error::<Test>::SubmissionPeriodTooShort
		);
	});
}

/// Live tenders keep the policy they were published under.
#[test]
fn a_policy_change_does_not_move_a_live_tender() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(0, 0, 0, Some(1))));
		assert_eq!(TenderChain::tenders(id).unwrap().policy, ProcurementPolicy::default());
		// The new one-block cap does not bind the tender published before it.
		assert_ok!(TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(20)));
	});
}

/// The governed addendum rule (spec §2.3): extensions are capped.
#[test]
fn addendum_extensions_are_capped_by_policy() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(0, 0, 0, Some(5))));
		let id = create_and_publish(0); // published close 10
		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(16)),
			Error::<Test>::CloseExtensionTooLong
		);
		assert_ok!(TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(15)));
		// The cap is measured from the published close, so it cannot be ratcheted.
		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x45u8; 32], Some(16)),
			Error::<Test>::CloseExtensionTooLong
		);
	});
}

/// The governed addendum rule (spec §2.3): a late addendum must give bidders
/// the response window back.
#[test]
fn a_late_addendum_must_restore_the_response_window() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::set_policy(RuntimeOrigin::root(), policy(0, 0, 4, None)));
		let id = create_and_publish(0); // close 10
		run_to_block(8); // two blocks left
		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], None),
			Error::<Test>::AddendumNeedsExtension
		);
		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(11)),
			Error::<Test>::AddendumNeedsExtension
		);
		assert_ok!(TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(12)));
	});
}

/// An extension shifts every downstream gate by the same amount, so a gap the
/// officer published between close and opening survives.
#[test]
fn an_extension_preserves_the_gap_before_opening() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Rft,
			BidMode::Sealed,
			TITLE.to_vec(),
			SUMMARY.to_vec(),
			NOTICE,
			CRITERIA,
			weights(),
			TenderGates {
				publish_at: 1,
				questions_close_at: 5,
				submission_close_at: 10,
				opening_at: 12,
				opening_end_at: 17,
			},
			20,
			vec![],
			0,
			bond(0),
			false,
			None,
		));
		let id = last_tender_id();
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [0x44u8; 32], Some(20)));
		let g = TenderChain::tenders(id).unwrap().gates;
		assert_eq!((g.submission_close_at, g.opening_at, g.opening_end_at), (20, 22, 27));
	});
}
