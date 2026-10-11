def apply_coupons(total, coupons):
    """Apply (kind, val) coupons in order; each acts on the running total.

    kind="percent": subtract val percent of the running total.
    kind="flat": subtract val dollars.
    """
    orig = total
    for kind, val in coupons:
        if kind == "percent":
            total -= orig * val / 100
        else:
            total -= val
    return round(max(total, 0.0), 2)
