from cart import cart_total
from discount import apply_coupons


def checkout(state, catalog, coupons):
    total = cart_total(state, catalog)
    final = apply_coupons(total, coupons)
    state["orders"].append({"total": final})
    state["cart"] = []
    return final
