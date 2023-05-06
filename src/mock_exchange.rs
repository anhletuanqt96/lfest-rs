//! Convenince function used in tests.

use fpdec::{Dec, Decimal};

use crate::{account_tracker::NoAccountTracker, prelude::*};

/// Constructs a mock exchange for testing.
pub fn mock_exchange() -> Exchange<NoAccountTracker, BaseCurrency> {
    let acc_tracker = NoAccountTracker::default();
    let contract_specification = ContractSpecification {
        ticker: "TESTUSD".to_string(),
        initial_margin: Dec!(0.01),
        maintenance_margin: Dec!(0.02),
        mark_method: MarkMethod::MidPrice,
        price_filter: PriceFilter::default(),
        quantity_filter: QuantityFilter::default(),
    };
    let config = Config::new(
        fee!(0.0002),
        fee!(0.0006),
        quote!(1000),
        200,
        leverage!(1),
        contract_specification,
    )
    .unwrap();
    Exchange::new(acc_tracker, config)
}