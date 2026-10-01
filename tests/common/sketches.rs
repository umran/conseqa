//! Sketches shared by the sketch tests.

#![allow(dead_code)]

/// The sketches a coordinator would write for `shop`.
pub fn shop_sketches() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "operation.place_order",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "product", "record": "object.product",
                  "by": { "product_id": "input.product_id" } },
                { "kind": "update", "record": "product", "set": ["stock"],
                  "from": ["product.stock", "input.quantity"] },
                { "kind": "create", "record": "object.order",
                  "from": ["input.request_id", "input.customer_id", "input.product_id",
                           "input.quantity"] }
            ]}),
        ),
        (
            "operation.pay_order",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order", "transition": "transition.order.pay",
                  "otherwise": "not_payable" },
                { "kind": "create", "record": "object.payment",
                  "from": ["input.request_id", "input.order_id", "input.amount"] }
            ]}),
        ),
        (
            "operation.cancel_order",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order",
                  "transition": "transition.order.cancel",
                  "otherwise": "not_cancellable", "already_ok": true },
                { "kind": "find", "as": "product", "record": "object.product",
                  "by": { "product_id": "order.product_id" } },
                { "kind": "update", "record": "product", "set": ["stock"],
                  "from": ["product.stock", "order.quantity"] }
            ]}),
        ),
        (
            "operation.restock",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "product", "record": "object.product",
                  "by": { "product_id": "input.product_id" } },
                { "kind": "update", "record": "product", "set": ["stock"],
                  "from": ["product.stock", "input.quantity"] },
                { "kind": "create", "record": "object.restock_receipt",
                  "from": ["input.request_id", "input.product_id", "input.quantity"] }
            ]}),
        ),
        (
            "operation.ship_order",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order", "transition": "transition.order.ship",
                  "otherwise": "not_shippable" }
            ]}),
        ),
    ]
}
