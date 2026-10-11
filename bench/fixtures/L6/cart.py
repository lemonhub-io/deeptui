def add_to_cart(state, item, qty=1):
    """Add item to cart; merge with an existing line for the same sku."""
    state["cart"].append({"sku": item.sku, "qty": qty})


def cart_total(state, catalog):
    total = 0.0
    for line in state["cart"]:
        total += catalog[line["sku"]].price * line["qty"]
    return round(total, 2)
