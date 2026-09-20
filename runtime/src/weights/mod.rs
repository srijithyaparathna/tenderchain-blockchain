//! Benchmarked weights for the runtime's pallets.
//!
//! Files in this module are produced by the Substrate benchmark CLI against
//! this runtime's own `Config` bounds and this machine's hardware. Do not edit
//! them by hand — regenerate with:
//!
//! ```text
//! cargo build --release --features runtime-benchmarks -p solochain-template-node
//! ./target/release/solochain-template-node benchmark pallet \
//!   --pallet pallet_tender_chain --extrinsic '*' --steps 50 --repeat 20 \
//!   --output runtime/src/weights/pallet_tender_chain.rs
//! ```
//!
//! Production weights should be regenerated on reference hardware; the numbers
//! committed here were measured on the development machine.

pub mod pallet_tender_chain;
