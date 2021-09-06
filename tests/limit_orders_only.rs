//! Test if a pure limit order strategy works correctly

use lfest::*;

#[test]
fn limit_orders_only() {
    if let Err(_) = pretty_env_logger::try_init() {}

    let config = Config::new(0.0002, 0.0006, 1.0, 1.0, FuturesTypes::Linear).unwrap();

    let mut exchange = Exchange::new(config);

    let _ = exchange.update_state(100.0, 100.1, 0, 100.1, 100.0);

    // TODO: intergation test limit_orders_only
}