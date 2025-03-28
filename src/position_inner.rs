use std::cmp::Ordering;

use const_decimal::Decimal;
use getset::{CopyGetters, Getters};
use num::One;
use num_traits::{Signed, Zero};
use tracing::{debug, trace};

use crate::{
    prelude::{
        Currency, EXCHANGE_FEE_ACCOUNT, Mon, QuoteCurrency, TREASURY_ACCOUNT, Transaction,
        TransactionAccounting, USER_POSITION_MARGIN_ACCOUNT, USER_WALLET_ACCOUNT,
    },
    types::MarginCurrency,
};

/// Describes the position information of the account.
/// It assumes isolated margining mechanism, because the margin is directly associated with the position.
#[derive(Debug, Clone, Default, Eq, PartialEq, Getters, CopyGetters)]
pub struct PositionInner<I, const D: u8, BaseOrQuote>
where
    I: Mon<D>,
    BaseOrQuote: Currency<I, D>,
{
    /// The number of futures contracts making up the position.
    #[getset(get_copy = "pub")]
    quantity: BaseOrQuote,

    /// The average price at which this position was entered at.
    #[getset(get_copy = "pub")]
    entry_price: QuoteCurrency<I, D>,

    /// The outstanding fees of the position that will be payed when reducing the position.
    #[getset(get_copy = "pub")]
    outstanding_fees: BaseOrQuote::PairedCurrency,
}

impl<I, const D: u8, BaseOrQuote> std::fmt::Display for PositionInner<I, D, BaseOrQuote>
where
    I: Mon<D>,
    BaseOrQuote: Currency<I, D>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PositionInner( quantity: {}, outstanding_fees: {})",
            self.quantity, self.outstanding_fees
        )
    }
}

impl<I, const D: u8, BaseOrQuote> PositionInner<I, D, BaseOrQuote>
where
    I: Mon<D>,
    BaseOrQuote: Currency<I, D>,
    BaseOrQuote::PairedCurrency: MarginCurrency<I, D>,
{
    #[cfg(test)]
    pub(crate) fn from_parts(
        quantity: BaseOrQuote,
        entry_price: QuoteCurrency<I, D>,
        outstanding_fees: BaseOrQuote::PairedCurrency,
    ) -> Self {
        Self {
            quantity,
            entry_price,
            outstanding_fees,
        }
    }

    /// Create a new instance.
    ///
    /// # Panics:
    /// if `quantity` or `entry_price` are invalid.
    pub fn new<Acc>(
        quantity: BaseOrQuote,
        entry_price: QuoteCurrency<I, D>,
        accounting: &mut Acc,
        init_margin_req: Decimal<I, D>,
        fees: BaseOrQuote::PairedCurrency,
    ) -> Self
    where
        Acc: TransactionAccounting<I, D, BaseOrQuote::PairedCurrency>,
    {
        trace!("new position: qty {quantity} @ {entry_price}");
        assert!(quantity > BaseOrQuote::zero());
        assert!(entry_price > QuoteCurrency::zero());
        debug_assert!(init_margin_req > Decimal::zero());
        debug_assert!(init_margin_req <= Decimal::one());

        // TODO: single function which computes this across the codebase.
        let margin =
            BaseOrQuote::PairedCurrency::convert_from(quantity, entry_price) * init_margin_req;
        let transaction =
            Transaction::new(USER_POSITION_MARGIN_ACCOUNT, USER_WALLET_ACCOUNT, margin);
        accounting
            .create_margin_transfer(transaction)
            .expect("margin transfer for opening a new position works.");

        Self {
            quantity,
            entry_price,
            outstanding_fees: fees,
        }
    }

    /// The cost of the position.
    #[inline(always)]
    pub fn total_cost(&self) -> BaseOrQuote::PairedCurrency {
        BaseOrQuote::PairedCurrency::convert_from(self.quantity, self.entry_price)
    }

    /// Return the positions unrealized profit and loss
    /// denoted in QUOTE when using linear futures,
    /// denoted in BASE when using inverse futures
    #[inline(always)]
    pub fn unrealized_pnl(
        &self,
        mark_to_market_price: QuoteCurrency<I, D>,
    ) -> BaseOrQuote::PairedCurrency {
        BaseOrQuote::PairedCurrency::pnl(self.entry_price(), mark_to_market_price, self.quantity)
    }

    /// Add contracts to the position.
    pub(crate) fn increase_contracts<Acc>(
        &mut self,
        qty: BaseOrQuote,
        entry_price: QuoteCurrency<I, D>,
        accounting: &mut Acc,
        init_margin_req: Decimal<I, D>,
        fees: BaseOrQuote::PairedCurrency,
    ) where
        Acc: TransactionAccounting<I, D, BaseOrQuote::PairedCurrency>,
    {
        debug!(
            "increase_contracts: qty: {qty} @ {entry_price}; self: {}",
            self
        );
        assert2::assert!(qty > BaseOrQuote::zero());
        assert2::assert!(entry_price > QuoteCurrency::zero());

        let value = BaseOrQuote::PairedCurrency::convert_from(qty, entry_price);
        let new_entry_price = QuoteCurrency::new_weighted_price(
            self.entry_price,
            *self.quantity.as_ref(),
            entry_price,
            *qty.as_ref(),
        );

        self.quantity += qty;
        self.entry_price = new_entry_price;
        self.outstanding_fees += fees;

        let margin = value * init_margin_req;
        let transaction =
            Transaction::new(USER_POSITION_MARGIN_ACCOUNT, USER_WALLET_ACCOUNT, margin);
        accounting
            .create_margin_transfer(transaction)
            .expect("is an internal call and must work");
    }

    /// Decrease the position.
    pub(crate) fn decrease_contracts<Acc>(
        &mut self,
        qty: BaseOrQuote,
        liquidation_price: QuoteCurrency<I, D>,
        accounting: &mut Acc,
        init_margin_req: Decimal<I, D>,
        direction_multiplier: i8,
        fees: BaseOrQuote::PairedCurrency,
    ) where
        Acc: TransactionAccounting<I, D, BaseOrQuote::PairedCurrency>,
    {
        debug!(
            "decrease_contracts: qty: {qty} @ {liquidation_price}; self: {}",
            self
        );
        assert2::assert!(qty > BaseOrQuote::zero());
        assert2::assert!(qty <= self.quantity);
        debug_assert!(direction_multiplier == 1 || direction_multiplier == -1);

        let entry_price = self.entry_price();

        self.quantity -= qty;
        self.outstanding_fees += fees;

        debug_assert!(self.quantity >= BaseOrQuote::zero());

        let pnl = BaseOrQuote::PairedCurrency::pnl(
            entry_price,
            liquidation_price,
            if direction_multiplier == 1 { qty } else { -qty },
        );
        match pnl.cmp(&BaseOrQuote::PairedCurrency::zero()) {
            Ordering::Greater => {
                let transaction = Transaction::new(USER_WALLET_ACCOUNT, TREASURY_ACCOUNT, pnl);
                accounting
                    .create_margin_transfer(transaction)
                    .expect("margin transfer must work");
            }
            Ordering::Less => {
                let transaction =
                    Transaction::new(TREASURY_ACCOUNT, USER_WALLET_ACCOUNT, pnl.abs());
                accounting
                    .create_margin_transfer(transaction)
                    .expect("margin transfer must work");
            }
            Ordering::Equal => {}
        }
        let margin_to_free =
            BaseOrQuote::PairedCurrency::convert_from(qty, entry_price) * init_margin_req;
        debug_assert!(margin_to_free > BaseOrQuote::PairedCurrency::zero());
        let transaction = Transaction::new(
            USER_WALLET_ACCOUNT,
            USER_POSITION_MARGIN_ACCOUNT,
            margin_to_free,
        );
        accounting
            .create_margin_transfer(transaction)
            .expect("margin transfer must work");

        if self.outstanding_fees > BaseOrQuote::PairedCurrency::zero() {
            let transaction = Transaction::new(
                EXCHANGE_FEE_ACCOUNT,
                USER_WALLET_ACCOUNT,
                self.outstanding_fees,
            );
            accounting
                .create_margin_transfer(transaction)
                .expect("margin transfer must work");
            self.outstanding_fees = BaseOrQuote::PairedCurrency::zero();
        }
    }
}

#[cfg(test)]
mod tests {
    use const_decimal::Decimal;
    use num_traits::One;

    use super::*;
    use crate::{DECIMALS, MockTransactionAccounting, prelude::*, test_fee_maker};

    #[test_case::test_matrix([1, 2, 5])]
    fn position_inner_new(leverage: u8) {
        let mut ta =
            InMemoryTransactionAccounting::new(QuoteCurrency::<i64, DECIMALS>::new(1000, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = BaseCurrency::new(5, 1);
        let entry_price = QuoteCurrency::new(100, 0);
        let fees = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        let pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fees);
        assert_eq!(
            pos,
            PositionInner {
                quantity: qty,
                entry_price,
                outstanding_fees: fees,
            }
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            QuoteCurrency::new(50, 0) * init_margin_req
        );
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0) - QuoteCurrency::new(50, 0) * init_margin_req
        );
    }

    #[test_case::test_matrix([1, 2, 5])]
    fn position_inner_increase_contracts(leverage: u8) {
        let mut ta = InMemoryTransactionAccounting::new(QuoteCurrency::<_, DECIMALS>::new(1000, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = BaseCurrency::new(5, 1);
        let entry_price = QuoteCurrency::new(100, 0);
        let fee_0 = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        let mut pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fee_0);

        let entry_price = QuoteCurrency::new(150, 0);
        let fee_1 = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        pos.increase_contracts(qty, entry_price, &mut ta, init_margin_req, fee_1);
        assert_eq!(
            pos,
            PositionInner {
                quantity: BaseCurrency::one(),
                entry_price: QuoteCurrency::new(125, 0),
                outstanding_fees: fee_0 + fee_1
            }
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(125, 0));
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            QuoteCurrency::new(125, 0) * init_margin_req
        );
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0) - QuoteCurrency::new(125, 0) * init_margin_req
        );
    }

    #[test_case::test_matrix([1, 2, 5])]
    fn position_inner_decrease_contracts_basic(leverage: u8) {
        let mut ta = InMemoryTransactionAccounting::new(QuoteCurrency::<_, DECIMALS>::new(1000, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = BaseCurrency::new(5, 0);
        let entry_price = QuoteCurrency::new(100, 0);
        let fees = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        let mut pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fees);
        pos.decrease_contracts(
            qty / BaseCurrency::new(2, 0),
            entry_price,
            &mut ta,
            init_margin_req,
            1,
            fees / QuoteCurrency::new(2, 0),
        );
        assert_eq!(
            pos,
            PositionInner {
                quantity: BaseCurrency::new(25, 1),
                entry_price: QuoteCurrency::new(100, 0),
                outstanding_fees: QuoteCurrency::new(0, 0),
            }
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        let margin = QuoteCurrency::new(250, 0) * init_margin_req;
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            margin,
        );
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0) - margin - fees * Decimal::try_from_scaled(15, 1).unwrap()
        );

        pos.decrease_contracts(
            qty / BaseCurrency::new(2, 0),
            entry_price,
            &mut ta,
            init_margin_req,
            1,
            fees / QuoteCurrency::new(2, 0),
        );
        assert_eq!(
            pos,
            PositionInner {
                quantity: BaseCurrency::new(0, 0),
                entry_price: QuoteCurrency::new(100, 0),
                outstanding_fees: QuoteCurrency::new(0, 0),
            }
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            QuoteCurrency::new(0, 0)
        );
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0) - fees * Decimal::try_from_scaled(2, 0).unwrap()
        );
    }

    #[test_case::test_matrix(
        [1, 2, 5],
        [Side::Buy, Side::Sell]
    )]
    fn position_inner_decrease_contracts_win(leverage: u8, position_side: Side) {
        let mut ta = InMemoryTransactionAccounting::new(QuoteCurrency::<_, DECIMALS>::new(1000, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = BaseCurrency::new(5, 0);
        let entry_price = QuoteCurrency::new(100, 0);
        let fees = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        let mut pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fees);

        let exit_price = QuoteCurrency::new(110, 0);
        let side_mult = match position_side {
            Side::Buy => 1,
            Side::Sell => -1,
        };
        pos.decrease_contracts(
            qty / BaseCurrency::new(2, 0),
            exit_price,
            &mut ta,
            init_margin_req,
            side_mult,
            fees / QuoteCurrency::new(2, 0),
        );

        assert_eq!(pos.quantity(), BaseCurrency::new(25, 1));
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        assert_eq!(pos.total_cost(), QuoteCurrency::new(250, 0));
        let margin = QuoteCurrency::new(250, 0) * init_margin_req;
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            margin
        );
        let profit = QuoteCurrency::new(25 * side_mult as i64, 0);
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0) + profit
                - margin
                - fees * Decimal::try_from_scaled(15, 1).unwrap()
        );
    }

    #[test_case::test_matrix(
        [1, 2, 5],
        [Side::Buy, Side::Sell]
    )]
    fn position_inner_decrease_contracts_2(leverage: u8, position_side: Side) {
        let mut ta = InMemoryTransactionAccounting::new(QuoteCurrency::<_, DECIMALS>::new(1000, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = BaseCurrency::new(5, 0);
        let entry_price = QuoteCurrency::new(100, 0);
        let fees = QuoteCurrency::convert_from(qty, entry_price) * *test_fee_maker().as_ref();
        let mut pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fees);

        let exit_price = QuoteCurrency::new(90, 0);
        let side_mult = match position_side {
            Side::Buy => 1,
            Side::Sell => -1,
        };
        pos.decrease_contracts(
            qty / BaseCurrency::new(2, 0),
            exit_price,
            &mut ta,
            init_margin_req,
            side_mult,
            fees / QuoteCurrency::new(2, 0),
        );

        assert_eq!(pos.quantity(), BaseCurrency::new(25, 1));
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        assert_eq!(pos.total_cost(), QuoteCurrency::new(250, 0));
        let margin = QuoteCurrency::new(250, 0) * init_margin_req;
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            margin
        );
        let loss = QuoteCurrency::new(25 * side_mult as i64, 0);
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            QuoteCurrency::new(1000, 0)
                - loss
                - margin
                - fees * Decimal::try_from_scaled(15, 1).unwrap()
        );
    }

    #[tracing_test::traced_test]
    #[test_case::test_matrix([1, 2, 5])]
    #[ignore]
    fn position_inner_decrease_contracts_inverse(leverage: u8) {
        let mut ta = InMemoryTransactionAccounting::new(BaseCurrency::<_, DECIMALS>::new(10, 0));
        let init_margin_req = Leverage::new(leverage).unwrap().init_margin_req();
        let qty = QuoteCurrency::new(500, 0);
        let entry_price = QuoteCurrency::new(100, 0);
        let val = BaseCurrency::convert_from(qty, entry_price);
        assert_eq!(val, BaseCurrency::new(5, 0));
        let fees = val * *test_fee_maker().as_ref();
        let mut pos = PositionInner::new(qty, entry_price, &mut ta, init_margin_req, fees);

        let exit_price = QuoteCurrency::new(200, 0);
        pos.decrease_contracts(
            qty / QuoteCurrency::new(2, 0),
            exit_price,
            &mut ta,
            init_margin_req,
            1,
            fees / BaseCurrency::new(2, 0),
        );

        assert_eq!(pos.quantity(), QuoteCurrency::new(250, 0));
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
        assert_eq!(pos.total_cost(), BaseCurrency::new(25, 1));
        let margin = BaseCurrency::new(25, 1) * init_margin_req;
        assert_eq!(
            ta.margin_balance_of(USER_POSITION_MARGIN_ACCOUNT).unwrap(),
            margin
        );
        assert_eq!(
            ta.margin_balance_of(USER_WALLET_ACCOUNT).unwrap(),
            BaseCurrency::new(1125, 2) - margin - fees * Decimal::try_from_scaled(15, 1).unwrap()
        );
    }

    #[test_case::test_matrix([1, 2, 5, 9])]
    fn position_inner_entry_price_linear(qty: i32) {
        let qty = BaseCurrency::<i32, DECIMALS>::new(qty, 0);
        let mut ta = InMemoryTransactionAccounting::new(QuoteCurrency::new(1000, 0));
        let init_margin_req = Decimal::one();
        let fees = QuoteCurrency::new(0, 0);
        let pos = PositionInner::new(
            qty,
            QuoteCurrency::new(100, 0),
            &mut ta,
            init_margin_req,
            fees,
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
    }

    #[test_case::test_matrix([10, 20, 50, 90])]
    fn position_inner_entry_price_inverse(qty: i32) {
        let qty = QuoteCurrency::<i32, DECIMALS>::new(qty, 0);
        let mut ta = InMemoryTransactionAccounting::new(BaseCurrency::new(10, 0));
        let init_margin_req = Decimal::one();
        let fees = BaseCurrency::new(0, 0);
        let pos = PositionInner::new(
            qty,
            QuoteCurrency::new(100, 0),
            &mut ta,
            init_margin_req,
            fees,
        );
        assert_eq!(pos.entry_price(), QuoteCurrency::new(100, 0));
    }

    #[test]
    fn position_inner_display() {
        let mut acc = MockTransactionAccounting::default();
        let pos = PositionInner::new(
            BaseCurrency::<i64, 1>::new(5, 1),
            QuoteCurrency::new(100, 0),
            &mut acc,
            Decimal::try_from_scaled(1, 0).unwrap(),
            QuoteCurrency::new(1, 1),
        );
        assert_eq!(
            &pos.to_string(),
            "PositionInner( quantity: 0.5 Base, outstanding_fees: 0.1 Quote)"
        );
    }
}
