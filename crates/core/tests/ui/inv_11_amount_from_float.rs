// SPDX-License-Identifier: Apache-2.0 OR MIT
// INV-11: an amount cannot be made from a floating-point number.
use bezent_core::Amount;

fn main() {
    let _amount: Amount = Amount::from(1.5f64);
}
