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

/// The sketches a coordinator would write for `tenant_ledger`.
pub fn tenant_ledger_sketches() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "operation.post_entry",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "tenant", "record": "object.tenant",
                  "by": { "tenant_id": "input.tenant_id" } },
                { "kind": "update", "record": "tenant", "set": ["last_sequence"],
                  "from": ["tenant.last_sequence"] },
                { "kind": "create", "record": "object.entry",
                  "from": ["input.tenant_id", "input.entry_id", "tenant.last_sequence",
                           "input.amount"] },
                { "kind": "enqueue", "outbox": "outbox.tenant_events",
                  "schema": "schema.EntryPosted",
                  "from": ["input.request_id", "input.tenant_id", "tenant.last_sequence",
                           "input.entry_id", "input.amount"] }
            ], "returns": ["input.entry_id", "tenant.last_sequence"] }),
        ),
        (
            "operation.relay_tenant_event",
            serde_json::json!({ "steps": [
                { "kind": "publish", "topic": "topic.tenant_events",
                  "schema": "schema.EntryPosted",
                  "from": ["input.event_id", "input.tenant_id", "input.sequence",
                           "input.entry_id", "input.amount"] }
            ]}),
        ),
        (
            "operation.apply_entry",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "ledger", "record": "object.tenant_ledger",
                  "by": { "tenant_id": "input.tenant_id" } },
                { "kind": "advance", "record": "ledger", "field": "last_applied_sequence",
                  "to": "input.sequence", "rule": "successor" },
                { "kind": "update", "record": "ledger", "set": ["balance"],
                  "from": ["ledger.balance", "input.amount"] }
            ]}),
        ),
        (
            "operation.notify_entry",
            serde_json::json!({ "steps": [
                { "kind": "call", "name": "webhook-gateway.deliver",
                  "identity": ["input.event_id"], "duplicates": "identical_per_identity",
                  "from": ["input.event_id", "input.tenant_id", "input.entry_id",
                           "input.amount"] }
            ]}),
        ),
    ]
}

/// The sketches a coordinator would write for `flash_checkout`.
pub fn flash_checkout_sketches() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "operation.create_order",
            serde_json::json!({ "steps": [
                { "kind": "create", "record": "object.order",
                  "from": ["input.order_id", "input.amount"] },
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.OrderCreated",
                  "from": ["input.order_id", "input.idempotency_key", "input.warehouse_id",
                           "input.sku", "input.quantity", "input.amount"] }
            ]}),
        ),
        (
            "operation.reserve_inventory",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "stock", "record": "object.stock",
                  "by": { "warehouse_id": "input.warehouse_id", "sku": "input.sku" } },
                { "kind": "update", "record": "stock", "set": ["reserved"],
                  "from": ["stock.reserved", "input.quantity"] },
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.InventoryReserved",
                  "from": ["input.order_id", "input.warehouse_id", "input.sku",
                           "input.quantity", "input.amount"] }
            ]}),
        ),
        (
            "operation.charge_payment",
            serde_json::json!({ "steps": [
                { "kind": "call", "name": "payment-provider.charge", "as": "charge",
                  "duplicates": "distinguishable",
                  "result": { "ok": "schema.ChargeAccepted",
                              "errors": { "declined": "schema.ChargeDeclined" } },
                  "on_ok": [
                    { "kind": "publish", "topic": "topic.order_events",
                      "schema": "schema.PaymentCaptured",
                      "from": ["input.event_id", "input.order_id", "input.amount"] } ],
                  "on_error": { "declined": [
                    { "kind": "publish", "topic": "topic.order_events",
                      "schema": "schema.PaymentFailed",
                      "from": ["input.event_id", "input.order_id"] } ] } }
            ]}),
        ),
        (
            "operation.cancel_order",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "transition", "record": "order",
                  "transition": "transition.order.cancel", "otherwise": "not_pending" },
                { "kind": "publish", "topic": "topic.order_events",
                  "schema": "schema.OrderCancelled", "from": ["input.order_id"] }
            ]}),
        ),
        (
            "operation.apply_payment",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "order", "record": "object.order",
                  "by": { "order_id": "input.order_id" } },
                { "kind": "advance", "record": "order", "field": "last_applied_sequence",
                  "to": "input.sequence", "rule": "successor" },
                { "kind": "transition", "record": "order",
                  "transition": "transition.order.mark_paid",
                  "effects_from": ["order.order_id", "input.event_id"] }
            ]}),
        ),
        (
            "operation.transfer_stock",
            serde_json::json!({ "steps": [
                { "kind": "find", "as": "source", "record": "object.stock",
                  "by": { "warehouse_id": "input.source_warehouse_id", "sku": "input.sku" } },
                { "kind": "find", "as": "destination", "record": "object.stock",
                  "by": { "warehouse_id": "input.destination_warehouse_id",
                          "sku": "input.sku" } },
                { "kind": "update", "record": "source", "set": ["on_hand"],
                  "from": ["source.on_hand", "input.quantity"] },
                { "kind": "update", "record": "destination", "set": ["on_hand"],
                  "from": ["destination.on_hand", "input.quantity"] }
            ]}),
        ),
    ]
}
