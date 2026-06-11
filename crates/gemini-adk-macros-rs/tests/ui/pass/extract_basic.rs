//! Anchor: a well-formed `#[derive(Extract)]` struct expands and compiles.

use gemini_adk_macros_rs::Extract;

#[derive(Extract)]
#[extract(name = "order", window = 3)]
#[allow(dead_code)]
struct Order {
    #[recognize(integer_near = ["want", "get"])]
    quantity: Option<i64>,
    #[recognize(one_of = ["pizza", "salad", "soda"])]
    item: Option<String>,
    #[recognize(yes_no)]
    #[extract(state = "order_confirmed")]
    confirmed: Option<bool>,
}

fn main() {
    let record = Order::extract();
    // The `state` override is honored; other fields keep their own name as key.
    assert_eq!(
        record.field_state_keys(),
        vec![
            ("quantity".to_string(), "quantity".to_string()),
            ("item".to_string(), "item".to_string()),
            ("confirmed".to_string(), "order_confirmed".to_string()),
        ]
    );
}
