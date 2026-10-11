import argparse
import json
import sys

from catalog import CATALOG
from storage import Storage
import cart as cartmod
import checkout as checkoutmod


def main():
    ap = argparse.ArgumentParser(prog="shop")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("add")
    p.add_argument("sku")
    p.add_argument("--qty", type=int, default=1)
    sub.add_parser("cart")
    p = sub.add_parser("checkout")
    p.add_argument("--coupon", action="append", default=[])
    sub.add_parser("catalog")
    # TODO: `wishlist` subcommands (add/list/move) — see README
    a = ap.parse_args()

    st = Storage()
    state = st.load()
    if a.cmd == "add":
        if a.sku not in CATALOG:
            sys.exit("no such sku")
        cartmod.add_to_cart(state, CATALOG[a.sku], a.qty)
        st.save(state)
        print("added")
    elif a.cmd == "cart":
        print(json.dumps(state["cart"], indent=1))
    elif a.cmd == "checkout":
        coupons = [(k, float(v)) for k, v in (c.split(":") for c in a.coupon)]
        print(checkoutmod.checkout(state, CATALOG, coupons))
        st.save(state)
    elif a.cmd == "catalog":
        for i in CATALOG.values():
            print(i.sku, i.name, i.price)


if __name__ == "__main__":
    main()
